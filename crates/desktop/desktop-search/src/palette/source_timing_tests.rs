//! Each source's rows show as soon as that source has them: a source that is slow, or one that
//! fails, holds back and hides nothing of the others. While a typed query has no row yet, the card
//! keeps the rows of the query before it, dimmed as stale, rather than collapsing between keys;
//! it says "Searching…" only when there were no rows to keep.
//!
//! On the page, a search over a hosted workspace said "Searching…" for 56 s and then "No results"
//! for text a file held: the rows that switch sources off were asked on the frame, a frame after
//! the rest, and the page drew that frame only when its clock turned the minute. The frame that
//! typed now asks the one source that holds the frame's state, and every other source answers on a
//! worker, whose answer wakes the shell.
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

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

/// Where the host's answer to one query is completed.
type Answer = tilo_actions::remote::Completer<
    Result<Vec<tilo_actions::remote::Found>, tilo_actions::remote::ActionCallError>,
>;

/// A workspace host whose answer the test hands over, as the session brings it.
#[derive(Default)]
struct Host {
    asked: Mutex<Vec<(String, Answer)>>,
}

impl tilo_actions::remote::ResourceSearch for Host {
    fn find(
        &self,
        _: &tilo_actions::remote::Caller,
        needle: &str,
    ) -> tilo_actions::remote::Pending<
        Result<Vec<tilo_actions::remote::Found>, tilo_actions::remote::ActionCallError>,
    > {
        let (done, answer) = tilo_actions::remote::pending();
        self.asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((needle.to_owned(), done));
        answer
    }
}

impl Host {
    /// Answer `needle` with the note that says it.
    fn answer(&self, needle: &str) {
        let done = {
            let mut asked = self
                .asked
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let at = asked
                .iter()
                .position(|(asked, _)| asked == needle)
                .expect("the host was asked");
            asked.remove(at).1
        };
        done.complete(Ok(vec![tilo_actions::remote::Found {
            resource: "notebook/pages/1".into(),
            kind: "notes.page".into(),
            label: "Supplier contract".into(),
            open: Some("open".into()),
            arguments: serde_json::Map::new(),
            app: Some("so.tilo.notes".into()),
            provider: None,
            when: None,
        }]));
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

/// A shell with Mail and Notes, a window, a command, a records source held until the returned
/// sender says so, and a files source that fails.
fn shell() -> (DesktopSearch, Sender<()>) {
    let (release, held) = channel();
    let mut data = super::tests::data();
    data.applications.push(AppEntry {
        id: "so.tilo.mail".into(),
        title: "Mail".into(),
        category: Some("Office".into()),
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

/// Draw frames, as the shell's wakes ask for them, until `done` holds; how many it took.
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
    // it: after that frame the search owes the shell no frame of its own, so a host that draws only
    // when woken (the page) shows every row as its worker's answer wakes it.
    {
        let (mut search, release) = shell();
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
        let (mut search, release) = shell();
        frames_until(&mut search, |search| !search.results().is_empty());
        search.set_query("mail");
        search.refresh();
        // The frame the workers' answers wake: the app and the command are in, while the records
        // source has not answered and the files source failed.
        let frames = frames_until(&mut search, |search| {
            let shown = titles(search);
            !search.palette().is_stale()
                && shown.iter().any(|title| title == "Mail")
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
        let (mut search, release) = shell();
        frames_until(&mut search, |search| !search.results().is_empty());
        let before = titles(&search);
        search.set_query("net 30");
        search.refresh();
        // Every source but the slow one answered with nothing it shows: the card still waits for
        // a row, and keeps the rows it showed, dimmed, rather than collapsing to "Searching…".
        for _ in 0..50 {
            search.refresh();
            assert!(
                search.palette().is_stale() && search.palette().is_searching(),
                "the rows shown are the query before, kept while the typed one is answered"
            );
            assert_eq!(titles(&search), before, "the card never collapses between keys");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(!search.settled(), "rows of another query are no answer to it");
        release.send(()).expect("the records source waits");
        frames_until(&mut search, |search| {
            titles(search).iter().any(|title| title.contains("net 30"))
        });
        assert!(!search.palette().is_stale(), "the typed query's rows replace them");
        frames_until(&mut search, |search| !search.answering());
        assert!(!search.palette().is_searching(), "the search is over");
    }

    phase a_failing_source_ends_in_no_results_not_searching;
    // Nothing matches, the files source failed and the records source answered nothing: the card
    // says nothing matched once every source is done, rather than searching for ever or showing
    // the failure.
    {
        let (mut search, release) = shell();
        drop(release);
        frames_until(&mut search, |search| !search.results().is_empty());
        search.set_query("xqzvwk");
        search.refresh();
        frames_until(&mut search, |search| !search.answering());
        frames_until(&mut search, |search| !search.palette().is_searching());
        assert!(
            search.results().is_empty() && !search.palette().is_stale(),
            "nothing matched: {:?}",
            titles(&search)
        );
        assert!(
            search.palette().empty_text().contains("xqzvwk"),
            "the card says nothing matched: {:?}",
            search.palette().empty_text()
        );
    }

    phase the_hosts_answer_shows_when_it_comes_and_the_card_waits_for_it;
    // A note this device never opened is found by the workspace's host: the card says it is
    // searching while only that answer is coming, and lists it when it comes.
    {
        let host = Arc::new(Host::default());
        let mut data = super::tests::data();
        data.sources.push(Held::new(crate::providers::hosted::HostSearch::new(
            Arc::clone(&host) as Arc<dyn tilo_actions::remote::ResourceSearch>,
            tilo_actions::remote::Caller::local_person("ana"),
            "w",
            Arc::new(|| {}),
        )));
        let mut search = DesktopSearch::new();
        search.open(data);
        search.set_room(600.0, 400.0);
        frames_until(&mut search, |search| !search.results().is_empty());
        let before = titles(&search);
        search.set_query("net 30");
        search.refresh();
        frames_until(&mut search, |search| !search.answering());
        assert!(
            search.palette().is_searching()
                && search.palette().is_stale()
                && titles(&search) == before,
            "only the host's answer is coming, and the rows before it stay: {:?}",
            titles(&search)
        );
        host.answer("net 30");
        frames_until(&mut search, |search| {
            titles(search).iter().any(|title| title == "Supplier contract")
        });
        assert!(!search.palette().is_stale(), "the host's row replaced them");
        assert!(!search.palette().is_searching(), "the search is over");
    }

    phase a_host_that_never_answers_ends_in_nothing_matched_not_searching;
    // The workspace's host is asked and never answers. The search waits for it only so long
    // ([`hosted::PATIENCE`]), and once that ends nothing is left to come: a card still saying
    // "Searching…" would go on saying it for as long as it stood, since no answer would arrive to
    // draw it again. The person reads that nothing matched, and what to try instead.
    {
        let host = Arc::new(Host::default());
        let mut data = super::tests::data();
        data.sources.push(Held::new(crate::providers::hosted::HostSearch::new(
            Arc::clone(&host) as Arc<dyn tilo_actions::remote::ResourceSearch>,
            tilo_actions::remote::Caller::local_person("ana"),
            "w",
            Arc::new(|| {}),
        )));
        let mut search = DesktopSearch::new();
        search.open(data);
        search.set_room(600.0, 400.0);
        frames_until(&mut search, |search| !search.results().is_empty());
        search.set_query("xqzvwk");
        search.refresh();
        // Nothing matched and the host is still being waited for: the card says it is searching,
        // over the rows of the query before.
        frames_until(&mut search, |search| {
            search.palette().is_searching() && search.palette().is_stale()
        });
        // The wait ends, and the frames after it draw a card that says nothing matched.
        frames_until(&mut search, |search| !search.waiting());
        frames_until(&mut search, |search| !search.palette().is_searching());
        assert!(
            search.results().is_empty() && !search.palette().is_stale(),
            "the kept rows go once nothing is left to come: {:?}",
            titles(&search)
        );
        assert!(
            search.palette().empty_text().contains("xqzvwk"),
            "the card says nothing matched: {:?}",
            search.palette().empty_text()
        );
        // What the shell publishes as the search still searching follows the card: a host that
        // reads it for its own frames is not told to keep looking for an answer that is not coming.
        assert!(
            !search.expecting(),
            "nothing is coming, so the host is not asked to look again"
        );
    }
}
