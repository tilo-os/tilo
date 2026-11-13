//! A door's `find` over the agent door never holds the desktop's loop.
//!
//! An outside client (an MCP `tilo_find` call) asks the session to search while its sources are
//! slow. The session carries the ask out on the loop's pass, so it must not wait for a source
//! there: the sources work on their own threads, the frames keep being laid out while the find is
//! in flight, and the find answers once the source does. A source that never answers is cut off at
//! the find's own deadline with the rows that came, never at the loop's.
//!
//! Every step is the production path: the find is asked over the agent door, and the frames are
//! the session's own passes (`Session::step`). The pass count between the ask and the answer is
//! what proves the loop never waited: the old code answered the ask with a `recv()` on the
//! search's channel, so no pass ran until the source was done.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::json;
use tilo_desktop_search::model::{Hit, Source, Target};
use tilo_desktop_search::{Held, ResourceKind};
use tilo_service::Questions;
use ui::search::system::MAX_PROVIDER_SCORE;

use tilo_session::{ActionPlane, Options, SearchOptions, Session};

use crate::common::{CANVAS_ENTRY, PERSON, Repl, on_screen, private_home, until};

/// The title a slow source answers with, and the needle that asks for it.
const SLOW_TITLE: &str = "Quarterlyreport ledger";
/// The needle: one the seeded apps do not answer, so the slow source's row is what the find
/// leads with.
const NEEDLE: &str = "quarterlyreport";

/// How long the source takes to answer: long enough that a loop that waited for it is seen not to
/// have laid out a frame in between, and short enough for the test to be quick.
const SLOW_FOR: Duration = Duration::from_millis(1500);

/// A source that answers after [`SLOW_FOR`], as a mailbox reading a distant server would. It says
/// when its query started and ends, so the test knows its ask was off the loop's thread.
struct Slow {
    started: Arc<AtomicBool>,
    ended: Arc<AtomicBool>,
    hits: Vec<Hit>,
}

impl Source for Slow {
    fn id(&self) -> &'static str {
        "slow"
    }

    fn max_score(&self) -> i32 {
        MAX_PROVIDER_SCORE
    }

    fn query(&self, _needle: &str) -> Vec<Hit> {
        self.started.store(true, Ordering::SeqCst);
        std::thread::sleep(SLOW_FOR);
        self.ended.store(true, Ordering::SeqCst);
        self.hits.clone()
    }
}

/// The one row the slow source answers with: a row an app publishes, so an agent's `find` names
/// the resource it is about, as a note's row does.
fn slow_hit() -> Hit {
    Hit {
        row: None,
        preview: None,
        faces: Vec::new(),
        outcome: None,
        kind: ResourceKind::Custom("report".into()),
        provider: "so.tilo.reports".into(),
        id: "tl:so.tilo.reports:quarterlyreport".into(),
        title: SLOW_TITLE.into(),
        subtitle: None,
        facts: Vec::new(),
        score: MAX_PROVIDER_SCORE,
        icon: None,
        target: Target::Invoke(Box::new(tilo_desktop_search::InvokeTarget::open(
            "so.tilo.reports",
            "",
            "tl:so.tilo.reports:quarterlyreport",
        ))),
    }
}

/// A headless session whose search mounts the slow source beside its own, its agent door at
/// `root`, this test's program admitted to act as the person, and the canvas in front.
fn session(root: &std::path::Path, slow: &Slow, started: &Arc<AtomicBool>) -> (Session, Repl) {
    let program = std::env::current_exe().expect("this test's program");
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: true,
            collab: false,
            services: vec![Box::new(crate::common::seeded_apps())],
            questions: Some(Questions::new()),
            action_plane: ActionPlane::At(root.join("plane.sock")),
            agent_door: Some(root.join("agent.sock")),
            admit_repl: vec![program],
            search: SearchOptions {
                extra_sources: vec![Held::new(Slow {
                    started: Arc::clone(started),
                    ended: Arc::clone(&slow.ended),
                    hits: slow.hits.clone(),
                })],
                ..SearchOptions::default()
            },
            ..Options::default()
        },
        &[tilo_compositor::OutputConfig::new("HEADLESS-1", 1280, 800)],
    )
    .expect("session");
    crate::common::show_preset(&mut session, "Frames");
    until(&mut session, "the canvas", |session| {
        on_screen(session, CANVAS_ENTRY).is_some()
    });
    let mut repl = Repl::connect(&root.join("agent.sock"));
    repl.opened(&mut session);
    (session, repl)
}

/// A find asked over the agent door answers once its source does, and no pass of the loop waits
/// for the source: the ask is carried out on the loop's pass, so a pass that waited for the source
/// would take as long as the source does, and the desktop would freeze as it did in the field. The
/// old code answered the ask with a blocking `recv()` on the search's channel, so exactly one pass
/// here would have lasted [`SLOW_FOR`]; the test fails that, naming the freeze.
#[test]
fn a_door_find_answers_while_the_loop_keeps_running() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let started = Arc::new(AtomicBool::new(false));
    let ended = Arc::new(AtomicBool::new(false));
    let slow = Slow {
        started: Arc::clone(&started),
        ended: Arc::clone(&ended),
        hits: vec![slow_hit()],
    };
    let (mut session, mut repl) = session(root.path(), &slow, &started);

    // The ask goes out on the door's own thread; the test drives the loop, as `Session::run`
    // does. The answer is read after the source has ended, so what is measured is the passes the
    // loop ran while the find was in flight.
    let asked = Instant::now();
    repl.send(
        "tools/call",
        &json!({"name": "tilo_find", "arguments": {"query": NEEDLE}}),
    );
    let mut passes = 0_usize;
    let mut longest = Duration::ZERO;
    let deadline = Instant::now()
        .checked_add(tilo_test_env::hang_bound())
        .expect("the wait's bound fits in an Instant");
    while !ended.load(Ordering::SeqCst) {
        assert!(
            Instant::now() < deadline,
            "the slow source did not answer: {passes} passes ran, and the find is still out"
        );
        let pass = Instant::now();
        session.step(Duration::from_millis(1)).expect("a frame");
        longest = longest.max(pass.elapsed());
        passes = passes.saturating_add(1);
    }
    assert!(
        started.load(Ordering::SeqCst),
        "the source was asked at all: a find over nothing proves no frame's rate"
    );
    assert!(
        passes >= 10,
        "the loop kept turning while the source worked: only {passes} passes ran in {SLOW_FOR:?}"
    );
    assert!(
        longest < SLOW_FOR / 2,
        "no pass waited for the source: the longest pass took {longest:?}, and the source took \
         {SLOW_FOR:?} — a pass that waits for one freezes the desktop"
    );

    // And the find answers once the source does, and not before it: it is the source's answer the
    // caller hears, so it takes at least as long as the source did.
    let reply = repl.answer(&mut session);
    assert!(
        asked.elapsed() >= SLOW_FOR,
        "the find answered only once the source did: it answered in {:?}, and the source takes \
         {SLOW_FOR:?}",
        asked.elapsed()
    );
    let result = reply
        .get("result")
        .unwrap_or_else(|| panic!("tilo_find answered: {reply}"));
    assert_eq!(result["isError"], false, "the find answers: {reply}");
}
