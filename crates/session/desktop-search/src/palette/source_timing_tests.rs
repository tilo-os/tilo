//! Each source's rows show as soon as that source has them: a source that is slow, or one that
//! fails, holds back and hides nothing of the others, and the card says "Searching…" only while no
//! row has come.
//!
//! On the page, a search over a hosted workspace said "Searching…" for 56 s and then "No results"
//! for text a file held: the rows that switch sources off were asked on the frame, a frame after
//! the rest, and the page drew that frame only when its clock turned the minute. The frame that
//! typed now asks the one source that holds the frame's state, and every other source answers on a
//! worker, whose answer wakes the desk.
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};

use super::*;
use crate::model::{Held, Hit, Source, Target};
use ui::time::Duration;

/// A workspace's content that answers only once the test lets it: a records index reading a
/// slow store, a model asked a question.
struct Slow {
    release: Mutex<Receiver<()>>,
}

impl Source for Slow {
    fn id(&self) -> &'static str {
        "records"
    }

    fn name(&self) -> String {
        "Records".into()
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        if needle.trim().is_empty() {
            return Vec::new();
        }
        // Held until released: the worker asking it waits, the frames never do. A test that
        // drops the sender lets it answer nothing.
        let released = self
            .release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recv();
        if released.is_err() {
            return Vec::new();
        }
        vec![row("records", "contract", "Contract with Acme, net 30")]
    }
}

/// A source whose query fails: its worker answers nothing for it.
struct Failing;

impl Source for Failing {
    fn id(&self) -> &'static str {
        "workspace-files"
    }

    fn name(&self) -> String {
        "Files".into()
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        assert!(
            needle.trim().is_empty(),
            "the files index could not be read"
        );
        Vec::new()
    }
}

fn row(provider: &str, id: &str, title: &str) -> Hit {
    Hit {
        row: None,
        preview: None,
        faces: Vec::new(),
        outcome: None,
        kind: ResourceKind::Record,
        provider: provider.into(),
        id: id.into(),
        title: title.into(),
        subtitle: None,
        facts: Vec::new(),
        score: ui::search::system::MAX_PROVIDER_SCORE,
        icon: None,
        target: Target::Copy { text: id.into() },
    }
}

/// A desk with Mail and Notes, a window, a command, a records source held until the returned
/// sender says so, and a files source that fails.
fn desk() -> (DesktopSearch, Sender<()>) {
    let (release, held) = channel();
    let mut data = super::tests::data();
    data.applications.push(AppEntry {
        id: "so.tilo.mail".into(),
        title: "Mail".into(),
        category: Some("Office".into()),
        description: None,
    });
    data.commands.push(crate::providers::CommandSpec {
        id: "mail-settings".into(),
        title: "Mail accounts".into(),
        subtitle: None,
        icon: None,
        unasked: false,
    });
    data.sources.push(Held::new(Slow {
        release: Mutex::new(held),
    }));
    data.sources.push(Held::new(Failing));
    let mut search = DesktopSearch::new();
    search.open(data);
    // The card's room, as a host gives it: the rows are cut to it.
    search.set_room(600.0, 400.0);
    (search, release)
}

fn titles(search: &DesktopSearch) -> Vec<String> {
    search
        .results()
        .iter()
        .map(|hit| hit.title.clone())
        .collect()
}

/// Draw frames, as the desk's wakes ask for them, until `done` holds; how many it took.
fn frames_until(search: &mut DesktopSearch, done: impl Fn(&DesktopSearch) -> bool) -> usize {
    let now = ui::time::now();
    let deadline = now.checked_add(Duration::from_secs(10)).unwrap_or(now);
    let mut frames = 0_usize;
    while !done(search) {
        assert!(
            ui::time::now() < deadline,
            "never came: {:?}",
            titles(search)
        );
        search.refresh();
        frames = frames.saturating_add(1);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    frames
}

tilo_test_env::scenario! {
    fn each_source_shows_its_rows_when_it_has_them;

    phase the_frame_that_types_leaves_no_source_to_a_later_frame;
    // The only source asked on the frame is the apps' own content, and the frame that typed asks
    // it: after that frame the search owes the desk no frame of its own, so a host that draws only
    // when woken (the page) shows every row as its worker's answer wakes it.
    {
        let (mut search, release) = desk();
        frames_until(&mut search, |search| !search.results().is_empty());
        search.set_query("mail");
        search.refresh();
        assert!(
            !search.filling_in(),
            "no source is left for a frame nobody asks for"
        );
        drop(release);
    }

    phase apps_windows_and_commands_show_while_a_slow_source_is_out;
    {
        let (mut search, release) = desk();
        frames_until(&mut search, |search| !search.results().is_empty());
        search.set_query("mail");
        search.refresh();
        // The frame the workers' answers wake: the app and the command are in, while the records
        // source has not answered and the files source failed.
        let frames = frames_until(&mut search, |search| {
            let shown = titles(search);
            shown.iter().any(|title| title == "Mail")
                && shown.iter().any(|title| title == "Mail accounts")
        });
        assert!(
            search.answering(),
            "the records source is still out: the rows shown did not wait for it"
        );
        assert!(
            !search.palette().is_searching(),
            "rows are in, so the card does not say it is searching: {:?}",
            titles(&search)
        );
        assert!(
            frames < 1000,
            "the rows came with the workers' answers, not after a slow source ({frames} frames)"
        );
        // The person opens Mail while the records source is still out.
        let mail = search
            .results()
            .iter()
            .position(|hit| hit.title == "Mail")
            .expect("Mail is a row");
        assert!(search.choose(mail, None), "Mail is chosen");
        assert!(
            matches!(
                search.take_choice(),
                Some(Target::Launch { app, .. }) if app == "so.tilo.mail"
            ),
            "choosing Mail opens Mail"
        );
        drop(release);
    }

    phase a_slow_sources_rows_join_the_ones_shown;
    {
        let (mut search, release) = desk();
        frames_until(&mut search, |search| !search.results().is_empty());
        search.set_query("net 30");
        search.refresh();
        // Every source but the slow one answered with nothing it shows: the card still waits for
        // a row, and says so.
        frames_until(&mut search, |search| {
            search.palette().is_searching() && search.results().is_empty()
        });
        release.send(()).expect("the records source waits");
        frames_until(&mut search, |search| {
            titles(search).iter().any(|title| title.contains("net 30"))
        });
        frames_until(&mut search, |search| !search.answering());
        assert!(!search.palette().is_searching(), "the search is over");
    }

    phase a_failing_source_ends_in_no_results_not_searching;
    // Nothing matches, the files source failed and the records source answered nothing: the card
    // says nothing matched once every source is done, rather than searching for ever or showing
    // the failure.
    {
        let (mut search, release) = desk();
        drop(release);
        frames_until(&mut search, |search| !search.results().is_empty());
        search.set_query("xqzvwk");
        search.refresh();
        frames_until(&mut search, |search| !search.answering());
        frames_until(&mut search, |search| !search.palette().is_searching());
        assert!(
            search.results().is_empty(),
            "nothing matched: {:?}",
            titles(&search)
        );
        assert!(
            search.palette().empty_text().contains("xqzvwk"),
            "the card says nothing matched: {:?}",
            search.palette().empty_text()
        );
    }
}
